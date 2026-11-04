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
//! - `GET /gateway`: every request the gateway's proxy received (`method`, `target` as received, so absolute-form
//!   `http://model/…`, `userAgent`, `status`); `GET /tools`: every request fake-tools answered (`method`, `path`,
//!   `userAgent`, the `Aviary-Sandbox-Person` and `Aviary-Sandbox-Conversation` the gateway added (`person`,
//!   `conversation`), whether it carried the tools pair bearer (`bearer`), the request `body` (as text, up to 1 MiB)
//!   and fake-tools' `status`, with its answer as text in `answer`);
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
    gateway: Arc<test_gateway::Running>,
    tools_seen: ToolsSeen,
}

/// What fake-tools was asked, as JSON objects.
type ToolsSeen = Arc<Mutex<Vec<Value>>>;

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
    let tools_seen = ToolsSeen::default();
    let tools_url = serve_tools(&tools_token, tools_seen.clone()).await;
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
            routes: Vec::new(),
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
    let gateway = Arc::new(gateway);
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
        gateway: gateway.clone(),
        tools_seen,
    });
    let app = admin_router(w);
    let _ = writeln!(std::io::stderr(), "fuse-rig: world up, proxy {}", gateway.proxy_addr());
    if let Err(e) = axum::serve(admin, app).await {
        fail(&e.to_string());
    }
    drop((hs, ms, gateway));
}

/// The admin port's routes (the module docs).
fn admin_router(w: Arc<World>) -> Router {
    Router::new()
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
        .route("/gateway", get(gateway_seen))
        .route("/tools", get(tools_seen_route))
        .with_state(w)
}

/// fake-tools behind the gateway's tools pair token, on loopback; its URL.
async fn serve_tools(token: &str, seen: ToolsSeen) -> String {
    let tools = fake_tools::FakeTools::new(token);
    let tl = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap_or_else(|e| fail(&e.to_string()));
    let url = format!("http://{}", tl.local_addr().unwrap_or_else(|e| fail(&e.to_string())));
    let token = token.to_owned();
    let app = tools.router().layer(axum::middleware::from_fn(
        move |req: axum::extract::Request, next: axum::middleware::Next| {
            let (seen, token) = (seen.clone(), token.clone());
            async move { record_tools(&seen, &token, req, next).await }
        },
    ));
    tokio::spawn(async move { axum::serve(tl, app).await });
    url
}

/// Records a request to fake-tools (its body and answer, which are small) and passes it on.
async fn record_tools(
    seen: &ToolsSeen,
    token: &str,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let (parts, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, 1 << 20).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let header = |name: &str| {
        parts
            .headers
            .get(name)
            .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
    };
    let ua = header("user-agent");
    let person = header("aviary-sandbox-person");
    let conversation = header("aviary-sandbox-conversation");
    let bearer = header("authorization").as_deref() == Some(format!("Bearer {token}").as_str());
    let (method, path) = (parts.method.to_string(), parts.uri.path().to_owned());
    let sent = String::from_utf8_lossy(&bytes).into_owned();
    let res = next
        .run(axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes)))
        .await;
    let status = res.status();
    let (rparts, rbody) = res.into_parts();
    let answer = axum::body::to_bytes(rbody, 1 << 20).await.unwrap_or_default();
    seen.lock().unwrap_or_else(PoisonError::into_inner).push(json!({
        "method": method,
        "path": path,
        "userAgent": ua,
        "person": person,
        "conversation": conversation,
        "bearer": bearer,
        "body": sent,
        "status": status.as_u16(),
        "answer": String::from_utf8_lossy(&answer),
    }));
    Response::from_parts(rparts, axum::body::Body::from(answer))
}

async fn gateway_seen(State(w): State<Arc<World>>) -> Json<Value> {
    let v: Vec<Value> = w
        .gateway
        .seen()
        .into_iter()
        .map(|s| json!({ "method": s.method, "target": s.target, "userAgent": s.user_agent, "status": s.status }))
        .collect();
    Json(Value::Array(v))
}

async fn tools_seen_route(State(w): State<Arc<World>>) -> Json<Value> {
    Json(Value::Array(
        w.tools_seen.lock().unwrap_or_else(PoisonError::into_inner).clone(),
    ))
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
