//! `fuse-rig world DIR [MODEL_URL [MEMORY_BYTES]]`: the world a real `supervisor` (under a real `supervisor-keeper`) runs against inside a
//! sandbox, for the re-run of the FUSE gate on the real build (`crates/supervisor/GATE.md`, WP-22): fake hermit and
//! fake magpie behind the reference gateway, all on loopback inside the sandbox, so their state moves with it in a
//! live migration. It writes `DIR/proxy` (the boot environment's `HTTP_PROXY`, with a fresh sandbox token) and
//! `DIR/cid`, then serves an admin port on `127.0.0.1:7300`:
//!
//! - `POST /hold/{route}`: hold every later call of a magpie route (`cc_write`, `cc_mkdir`, …) until released;
//! - `POST /release`: release and remove every hold; `GET /held`: how many calls wait at a hold now;
//! - `GET /calls/{route}`: every call of a route so far (query and body, so the `opId`s);
//! - `GET /node?path=P`: magpie's copy of a `~/.claude` path (`kind`, `size`, `sha256`, `version`), 404 when absent;
//!   `GET /data?path=P`: its bytes; `GET /paths`: every path;
//! - `POST /latency/{route}?ms=N`: every later call of a magpie route waits N ms first (`/release` removes it too);
//! - `POST /env`: merge a JSON object into the descriptor's `env` (hermit's `SandboxSelf.env`; a new `etag`, so the
//!   supervisor applies it at the next spawn);
//! - `POST /turn`: queue a `start_turn` command (`{"text", "resume"}`: `resume` the provider session id or null),
//!   answered with `{"turnId", "cmdSeq"}`; the turn's reports are `GET /calls/turn_state` (the `turnId` in `params`).
//!
//! Claude Code's model is the gate's scripted mock (`fuse-gate model`, at the optional second argument,
//! default `http://127.0.0.1:7200`) behind the gateway's `model` name, and its tools are `fake-tools` behind `tools`; the
//! descriptor's `env` carries the proxy URL (`HTTP_PROXY` and the other three spellings), as YYC's does.
//! `MEMORY_BYTES` replaces the descriptor's `resources.memoryBytes` (the supervisor's memory guard, supervisor 5,
//! counts the world too, since it runs inside the sandbox).

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
use magpie_api::sandbox::{CommandBody, Resume, StartTurn};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Digest as _;
use uuid::Uuid;

const ADMIN: &str = "127.0.0.1:7300";

struct World {
    magpie: Arc<FakeMagpie>,
    hermit: Arc<FakeHermit>,
    cid: Uuid,
    holds: Mutex<Vec<(FaultId, Hold)>>,
    latencies: Mutex<Vec<FaultId>>,
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
        let _ = writeln!(
            std::io::stderr(),
            "usage: fuse-rig world DIR [MODEL_URL [MEMORY_BYTES]]"
        );
        std::process::exit(2);
    };
    let dir = PathBuf::from(dir);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap_or_else(|e| fail(&e.to_string()));
    let model_url = args.get(3).cloned().unwrap_or_else(|| "http://127.0.0.1:7200".into());
    let memory: Option<u64> = args.get(4).map(|m| m.parse().unwrap_or_else(|_| fail("MEMORY_BYTES")));
    rt.block_on(world(dir, model_url, memory));
}

async fn world(dir: PathBuf, model_url: String, memory: Option<u64>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let hermit = FakeHermit::with_config(HermitConfig::default());
    let magpie = FakeMagpie::new();
    let hs = hermit.serve().await.unwrap_or_else(|e| fail(&e.to_string()));
    let ms = magpie.serve().await.unwrap_or_else(|e| fail(&e.to_string()));
    let tools_token = token_of("t");
    let tools_url = serve_tools(&tools_token).await;
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
    let token = token_of;
    let mut registry = prometheus_client::registry::Registry::default();
    let addr = |s: &str| s.parse().unwrap_or_else(|_| fail("address"));
    let gateway = test_gateway::start(
        test_gateway::Config {
            proxy_listen: addr("127.0.0.1:0"),
            api_listen: addr("127.0.0.1:0"),
            upstreams: test_gateway::Upstreams {
                magpie: ms.url(),
                hermit: hs.url(),
                tools: tools_url,
                model: model_url,
            },
            credentials: test_gateway::Credentials {
                magpie: magpie.token(Caller::Gateway).unwrap_or_else(|| fail("magpie token")),
                hermit: hermit.token(Caller::Gateway).unwrap_or_else(|| fail("hermit token")),
                tools: tools_token,
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
    configure_descriptor(&hermit, &proxy, memory);
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
        hermit,
        cid,
        holds: Mutex::new(Vec::new()),
        latencies: Mutex::new(Vec::new()),
    });
    let app = Router::new()
        .route("/hold/{route}", post(hold))
        .route("/release", post(release))
        .route("/held", get(held))
        .route("/calls/{route}", get(calls))
        .route("/node", get(node))
        .route("/data", get(data))
        .route("/paths", get(paths))
        .route("/latency/{route}", post(latency))
        .route("/env", post(env))
        .route("/turn", post(turn))
        .with_state(w);
    let _ = writeln!(std::io::stderr(), "fuse-rig: world up, proxy {}", gateway.proxy_addr());
    if let Err(e) = axum::serve(admin, app).await {
        fail(&e.to_string());
    }
    drop((hs, ms, gateway));
}

/// fake-tools behind the gateway's tools pair token, on loopback; its URL.
async fn serve_tools(token: &str) -> String {
    let tools = fake_tools::FakeTools::new(token);
    let tl = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap_or_else(|e| fail(&e.to_string()));
    let url = format!("http://{}", tl.local_addr().unwrap_or_else(|e| fail(&e.to_string())));
    let app = tools.router();
    tokio::spawn(async move { axum::serve(tl, app).await });
    url
}

/// The descriptor's proxy URL (the four spellings) and, when given, its `memoryBytes`.
fn configure_descriptor(hermit: &FakeHermit, proxy: &str, memory: Option<u64>) {
    hermit.configure(|c| {
        if let Some(m) = memory {
            c.descriptor.resources.memory_bytes = m;
        }
        for k in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            c.descriptor.env.insert(k.into(), proxy.to_owned());
        }
    });
}

fn token_of(n: &str) -> String {
    format!("{n}-{}", Uuid::new_v4().simple())
}

async fn latency(
    State(w): State<Arc<World>>,
    Path(route): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> StatusCode {
    let ms: u64 = q.get("ms").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut l = w.latencies.lock().unwrap_or_else(PoisonError::into_inner);
    for id in l.drain(..) {
        w.magpie.faults().remove(id);
    }
    if ms > 0 {
        l.push(
            w.magpie
                .faults()
                .on(&route, FaultKind::Latency(std::time::Duration::from_millis(ms))),
        );
    }
    StatusCode::NO_CONTENT
}

async fn env(State(w): State<Arc<World>>, Json(add): Json<HashMap<String, String>>) -> StatusCode {
    w.hermit.configure(|c| c.descriptor.env.extend(add));
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
struct TurnRequest {
    text: String,
    #[serde(default)]
    resume: Option<String>,
}

async fn turn(State(w): State<Arc<World>>, Json(t): Json<TurnRequest>) -> Json<Value> {
    let turn_id = Uuid::now_v7();
    let seq = w.magpie.push_command(
        w.cid,
        Some(turn_id),
        CommandBody::StartTurn(StartTurn {
            text: t.text,
            files: vec![],
            images: vec![],
            resume: Resume {
                provider_session_id: t.resume,
            },
            sent_at: magpie_api::Timestamp::now(),
        }),
    );
    Json(json!({ "turnId": turn_id, "cmdSeq": seq }))
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
    for id in std::mem::take(&mut *w.latencies.lock().unwrap_or_else(PoisonError::into_inner)) {
        w.magpie.faults().remove(id);
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
        .map(|c| json!({ "params": c.params, "query": c.query, "body": c.body, "bytes": c.bytes.map(|b| b.len()) }))
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
