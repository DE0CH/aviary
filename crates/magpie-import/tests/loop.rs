//! The hand-over loop (cutover 7.12 item 5): a fake Kubernetes API (ui pods, one Terminating; `pods/exec` answering
//! `holds.js` with each pod's children; `pods/proxy` answering the switch module's `/__jarvis/sockets`), a fake
//! `jarvis.sandboxes`, and real magpie and `cloudcli` databases. A conversation is released only under every condition
//! of DESIGN 9.4; a running one is never released while its idle sibling is; a schema with an unanswered child releases
//! nothing; a missing or unclean report keeps the gate unset; the running mirror follows the reads.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use common::{World, assistant, user};
use magpie_import::handover::{self, Handover, Wait};
use magpie_import::kube::Kube;
use uuid::Uuid;

const ALICE: &str = "alice@example.com";

#[derive(Default)]
struct Old {
    /// pod → terminating
    pods: Vec<(String, bool)>,
    /// pod → its children's lines (schema, ok, sessions)
    children: HashMap<String, Vec<(String, bool, Vec<String>)>>,
    /// pod → (non-LU sockets, non-LU http); absent: no report (404)
    reports: HashMap<String, (u64, u64)>,
}

type Fake = Arc<Mutex<Old>>;

fn authorised(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some("Bearer t0ken")
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
    if !authorised(&h) || h.get("x-jarvis-switch-token").and_then(|v| v.to_str().ok()) != Some("sw") {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(pod) = pod.strip_suffix(":8080") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some((sockets, http)) = f.lock().unwrap().reports.get(pod).copied() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    axum::Json(serde_json::json!({
        "generatedAt": now, "armed": true, "persons": {},
        "totals": {"sockets": sockets, "opaque": 0, "lu": 2, "luOpaque": 0, "http": http, "passOnInFlight": 0,
                   "hopSockets": 0, "hopOpaque": 0},
        "counters": {}
    }))
    .into_response()
}

async fn serve(f: Fake) -> String {
    let app = axum::Router::new()
        .route("/api/v1/namespaces/jarvis/pods", get(list))
        .route("/api/v1/namespaces/jarvis/pods/{pod}/exec", get(exec))
        .route(
            "/api/v1/namespaces/jarvis/pods/{pod}/proxy/__jarvis/sockets",
            get(sockets),
        )
        .with_state(f);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await });
    format!("http://{addr}")
}

async fn conversation(w: &World, ka: &str, title: &str) -> Uuid {
    let (u, p) = (Uuid::new_v4(), Uuid::new_v4());
    w.cloudcli(&format!(
        "insert into cc_{ka}.sessions (session_id, provider_session_id, custom_name, name_source, created_at,
                                       updated_at)
           values ('{u}', '{p}', '{title}', 'user', '2026-10-22 06:22:36', '2026-10-22T07:00:00.000Z');"
    ))
    .await;
    let main = format!("projects/-workspace/{p}.jsonl");
    w.put(
        ka,
        &main,
        &[
            user("a1", "2026-10-22T06:22:40.000Z", "write the report"),
            assistant("a2", "2026-10-22T06:23:00.000Z", "here it is"),
        ]
        .concat(),
    );
    u
}

async fn released(w: &World, id: Uuid) -> Option<String> {
    let r = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one(
            "select s.release_reason, c.migrating from import_sources s join conversations c on c.id = s.conversation_id
             where s.conversation_id = $1",
            &[&id],
        )
        .await
        .unwrap();
    let reason: Option<String> = r.get(0);
    assert_eq!(
        reason.is_some(),
        !r.get::<_, bool>(1),
        "released_at and migrating agree"
    );
    reason
}

async fn run_state(w: &World, id: Uuid) -> String {
    w.magpie()
        .get()
        .await
        .unwrap()
        .query_one("select run_state from conversations where id = $1", &[&id])
        .await
        .unwrap()
        .get(0)
}

async fn waits(h: &Handover, id: Uuid) -> Option<Wait> {
    h.status
        .read()
        .await
        .open
        .iter()
        .find(|o| o.conversation == id)
        .map(|o| o.waiting_for)
}

#[tokio::test]
async fn the_loop_releases_only_under_every_condition() {
    let w = World::new("loop_release").await;
    w.jarvis("create table sandboxes (id uuid primary key, conversation_id uuid not null, status text not null)")
        .await;
    let ka = w.person(ALICE).await;
    let schema = format!("cc_{ka}");
    let idle = conversation(&w, &ka, "Idle").await;
    let busy = conversation(&w, &ka, "Busy").await;
    let fake: Fake = Arc::new(Mutex::new(Old::default()));
    {
        let mut o = fake.lock().unwrap();
        o.pods = vec![("ui-a".into(), false), ("ui-b".into(), true)];
        o.children
            .insert("ui-a".into(), vec![(schema.clone(), true, vec![busy.to_string()])]);
        o.children.insert("ui-b".into(), vec![]);
        o.reports.insert("ui-a".into(), (0, 0));
        // ui-b (Terminating) has no report yet
    }
    let base = serve(fake.clone()).await;
    let kube = Kube::new(&base, "t0ken", None).unwrap();
    let mut h = Handover::new(kube.clone(), "sw".into());

    // before the gate: imported, verified, mirrored, never released
    for _ in 0..3 {
        h.pass(&w.imp).await.unwrap();
    }
    assert_eq!(released(&w, idle).await, None);
    assert_eq!(waits(&h, idle).await, Some(Wait::Gate));
    assert_eq!(run_state(&w, busy).await, "running");
    assert_eq!(run_state(&w, idle).await, "idle");

    // the gate refuses while a pod (the Terminating one) has no report
    let (check, set) = handover::allow(&w.imp, &kube, "sw").await.unwrap();
    assert!(
        !set && check.problems().iter().any(|p| p.starts_with("ui-b")),
        "{:?}",
        check.problems()
    );
    fake.lock().unwrap().reports.insert("ui-b".into(), (0, 0));
    assert!(handover::allow(&w.imp, &kube, "sw").await.unwrap().1);

    // a socket that is not LU on any pod holds every release
    fake.lock().unwrap().reports.insert("ui-a".into(), (1, 0));
    h.pass(&w.imp).await.unwrap();
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, idle).await, None);
    assert_eq!(waits(&h, idle).await, Some(Wait::Gate));
    // HTTP that is not LU in flight, the same
    fake.lock().unwrap().reports.insert("ui-a".into(), (0, 1));
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, idle).await, None);
    fake.lock().unwrap().reports.insert("ui-a".into(), (0, 0));

    // an old sandbox not gone holds it
    w.jarvis(&format!(
        "insert into sandboxes values ('{}', '{idle}', 'ready')",
        Uuid::new_v4()
    ))
    .await;
    h.pass(&w.imp).await.unwrap();
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, idle).await, None);
    assert_eq!(waits(&h, idle).await, Some(Wait::OldSandbox));
    w.jarvis("update sandboxes set status = 'gone'").await;

    // a child of the schema that did not answer holds both
    fake.lock()
        .unwrap()
        .children
        .insert("ui-b".into(), vec![(schema.clone(), false, vec![])]);
    h.pass(&w.imp).await.unwrap();
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, idle).await, None);
    assert_eq!(waits(&h, idle).await, Some(Wait::HoldsUnanswered));
    fake.lock().unwrap().children.insert("ui-b".into(), vec![]);

    // every condition: the idle one goes (after two clean reads); its running sibling stays
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, idle).await, None, "one clean read is not two");
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, idle).await.as_deref(), Some("idle"));
    assert_eq!(released(&w, busy).await, None);
    assert_eq!(waits(&h, busy).await, Some(Wait::Running));
    assert_eq!(h.status.read().await.released_total, 1);

    // it stops running: two reads later it goes too
    fake.lock()
        .unwrap()
        .children
        .insert("ui-a".into(), vec![(schema.clone(), true, vec![])]);
    h.pass(&w.imp).await.unwrap();
    assert_eq!(run_state(&w, busy).await, "idle");
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, busy).await.as_deref(), Some("idle"));
}

#[tokio::test]
async fn a_source_change_inside_the_fence_window_keeps_it_legacy() {
    let w = World::new("loop_change").await;
    w.jarvis("create table sandboxes (id uuid primary key, conversation_id uuid not null, status text not null)")
        .await;
    let ka = w.person(ALICE).await;
    let id = conversation(&w, &ka, "Changing").await;
    let fake: Fake = Arc::new(Mutex::new(Old::default()));
    {
        let mut o = fake.lock().unwrap();
        o.pods = vec![("ui-a".into(), false)];
        o.reports.insert("ui-a".into(), (0, 0));
    }
    let kube = Kube::new(&serve(fake.clone()).await, "t0ken", None).unwrap();
    let mut h = Handover::new(kube.clone(), "sw".into());
    h.pass(&w.imp).await.unwrap();
    assert!(handover::allow(&w.imp, &kube, "sw").await.unwrap().1);
    // the old side writes between passes: the pass syncs it, verification is stale, nothing is released
    let p: String = w
        .magpie()
        .get()
        .await
        .unwrap()
        .query_one(
            "select provider_session from import_sources where conversation_id = $1",
            &[&id],
        )
        .await
        .unwrap()
        .get(0);
    w.append(
        &ka,
        &format!("projects/-workspace/{p}.jsonl"),
        &user("a3", "2026-10-22T06:30:00.000Z", "one more"),
    );
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, id).await, None);
    // stable again: re-verified, then released
    h.pass(&w.imp).await.unwrap();
    h.pass(&w.imp).await.unwrap();
    assert_eq!(released(&w, id).await.as_deref(), Some("idle"));
}
