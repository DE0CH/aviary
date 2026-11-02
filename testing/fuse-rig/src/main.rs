//! `fuse-rig world DIR`: the world a real `supervisor` (under a real `supervisor-keeper`) runs against inside a
//! sandbox, for the re-run of the FUSE gate on the real build (`crates/supervisor/GATE.md`, WP-22): fake hermit and
//! fake magpie behind the reference gateway, all on loopback inside the sandbox, so their state moves with it in a
//! live migration. It writes `DIR/proxy` (the boot environment's `HTTP_PROXY`, with a fresh sandbox token) and
//! `DIR/cid`, then serves an admin port on `127.0.0.1:7300`:
//!
//! - `POST /hold/{route}`: hold every later call of a magpie route (`cc_write`, `cc_mkdir`, …) until released;
//! - `POST /release`: release and remove every hold; `GET /held`: how many calls wait at a hold now;
//! - `GET /calls/{route}`: every call of a route so far (query and body, so the `opId`s);
//! - `GET /node?path=P`: magpie's copy of a `~/.claude` path (`kind`, `size`, `sha256`, `version`), 404 when absent;
//!   `GET /data?path=P`: its bytes; `GET /paths`: every path.

use std::collections::HashMap;
use std::io::Write as _;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use aviary_core::auth::{Caller, Callers};
use aviary_fakes::fault::{FaultId, FaultKind, Hold};
use aviary_fakes::hermit::{FakeHermit, HermitConfig};
use aviary_fakes::magpie::FakeMagpie;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use hermit_api::sandbox::PutSandboxRequest;
use serde_json::{Value, json};
use sha2::Digest as _;
use uuid::Uuid;

const ADMIN: &str = "127.0.0.1:7300";

struct World {
    magpie: Arc<FakeMagpie>,
    cid: Uuid,
    holds: Mutex<Vec<(FaultId, Hold)>>,
}

fn fail(msg: &str) -> ! {
    let _ = writeln!(std::io::stderr(), "fuse-rig: {msg}");
    std::process::exit(1);
}

fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter().fold(String::new(), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some("world"), Some(dir)) = (args.get(1).map(String::as_str), args.get(2)) else {
        let _ = writeln!(std::io::stderr(), "usage: fuse-rig world DIR");
        std::process::exit(2);
    };
    let dir = PathBuf::from(dir);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap_or_else(|e| fail(&e.to_string()));
    rt.block_on(world(dir));
}

async fn world(dir: PathBuf) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let hermit = FakeHermit::with_config(HermitConfig::default());
    let magpie = FakeMagpie::new();
    let hs = hermit.serve().await.unwrap_or_else(|e| fail(&e.to_string()));
    let ms = magpie.serve().await.unwrap_or_else(|e| fail(&e.to_string()));
    let cid = Uuid::now_v7();
    let put = hs
        .client(reqwest::Client::new(), Caller::Magpie)
        .put_sandbox(
            cid,
            &PutSandboxRequest {
                person: "staff@example.org".into(),
            },
        )
        .await
        .unwrap_or_else(|e| fail(&format!("put_sandbox: {e}")));
    let _ = put.sandbox().id;
    let token = |n: &str| format!("{n}-{}", Uuid::new_v4().simple());
    let mut registry = prometheus_client::registry::Registry::default();
    let addr = |s: &str| s.parse().unwrap_or_else(|_| fail("address"));
    let gateway = test_gateway::start(
        test_gateway::Config {
            proxy_listen: addr("127.0.0.1:0"),
            api_listen: addr("127.0.0.1:0"),
            upstreams: test_gateway::Upstreams {
                magpie: ms.url(),
                hermit: hs.url(),
                tools: "http://127.0.0.1:9".into(),
                model: "http://127.0.0.1:9".into(),
            },
            credentials: test_gateway::Credentials {
                magpie: magpie.token(Caller::Gateway).unwrap_or_else(|| fail("magpie token")),
                hermit: hermit.token(Caller::Gateway).unwrap_or_else(|| fail("hermit token")),
                tools: token("t"),
                model: token("m"),
            },
            callers: Callers::new([(Caller::Pigeon, vec![token("p")]), (Caller::Hermit, vec![token("h")])])
                .unwrap_or_else(|e| fail(&e.to_string())),
            self_hosts: vec![],
            ops_summary: test_gateway::default_ops_summary(),
            drain: test_gateway::DrainBounds::default(),
            upload_cap: test_gateway::UPLOAD_CAP,
            upstream_idle: gateway_api::limits::IDLE_TIMEOUT,
            faults: test_gateway::Faults::default(),
        },
        &mut registry,
    )
    .await
    .unwrap_or_else(|e| fail(&e.to_string()));
    gateway.keys_loaded().await;
    let st = hermit
        .mint_token(cid, Ipv4Addr::LOCALHOST)
        .unwrap_or_else(|| fail("sandbox token"));
    let proxy = format!("http://sbx:{st}@{}", gateway.proxy_addr());
    let admin = tokio::net::TcpListener::bind(ADMIN)
        .await
        .unwrap_or_else(|e| fail(&format!("bind {ADMIN}: {e}")));
    let _ = std::fs::create_dir_all(&dir);
    let write = |name: &str, v: &str| {
        let tmp = dir.join(format!("{name}.tmp"));
        std::fs::write(&tmp, v)
            .and_then(|()| std::fs::rename(&tmp, dir.join(name)))
            .unwrap_or_else(|e| fail(&e.to_string()));
    };
    write("cid", &cid.to_string());
    write("proxy", &proxy);
    let w = Arc::new(World {
        magpie,
        cid,
        holds: Mutex::new(Vec::new()),
    });
    let app = Router::new()
        .route("/hold/{route}", post(hold))
        .route("/release", post(release))
        .route("/held", get(held))
        .route("/calls/{route}", get(calls))
        .route("/node", get(node))
        .route("/data", get(data))
        .route("/paths", get(paths))
        .with_state(w);
    let _ = writeln!(std::io::stderr(), "fuse-rig: world up, proxy {}", gateway.proxy_addr());
    if let Err(e) = axum::serve(admin, app).await {
        fail(&e.to_string());
    }
    drop((hs, ms, gateway));
}

async fn hold(State(w): State<Arc<World>>, Path(route): Path<String>) -> StatusCode {
    let h = Hold::new();
    let id = w.magpie.faults().on(&route, FaultKind::Hold(h.clone()));
    w.holds.lock().unwrap_or_else(PoisonError::into_inner).push((id, h));
    StatusCode::NO_CONTENT
}

async fn release(State(w): State<Arc<World>>) -> StatusCode {
    let hs = std::mem::take(&mut *w.holds.lock().unwrap_or_else(PoisonError::into_inner));
    for (id, h) in hs {
        w.magpie.faults().remove(id);
        h.release();
    }
    StatusCode::NO_CONTENT
}

async fn held(State(w): State<Arc<World>>) -> Json<Value> {
    let n: usize = w
        .holds
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|(_, h)| h.waiting())
        .sum();
    Json(json!({ "held": n }))
}

async fn calls(State(w): State<Arc<World>>, Path(route): Path<String>) -> Json<Value> {
    let v: Vec<Value> = w
        .magpie
        .script()
        .calls_to(&route)
        .into_iter()
        .map(|c| json!({ "query": c.query, "body": c.body, "bytes": c.bytes.map(|b| b.len()) }))
        .collect();
    Json(Value::Array(v))
}

fn path_of(q: &HashMap<String, String>) -> String {
    q.get("path").cloned().unwrap_or_default()
}

async fn node(State(w): State<Arc<World>>, Query(q): Query<HashMap<String, String>>) -> Response {
    match w.magpie.node(w.cid, &path_of(&q)) {
        Some(n) => Json(json!({
            "kind": format!("{:?}", n.kind),
            "size": n.data.len(),
            "sha256": hex(&sha2::Sha256::digest(&n.data)),
            "version": n.version,
            "mode": n.mode,
        }))
        .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn data(State(w): State<Arc<World>>, Query(q): Query<HashMap<String, String>>) -> Response {
    match w.magpie.node(w.cid, &path_of(&q)) {
        Some(n) => n.data.into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn paths(State(w): State<Arc<World>>) -> Json<Value> {
    Json(json!(w.magpie.paths(w.cid)))
}
