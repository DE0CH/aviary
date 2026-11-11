//! `aviary-gateway`: each aviary session machine's half of the sandbox gateway, on 127.0.0.1:3128 (the supervisor's
//! and Claude Code's `HTTP_PROXY`). The machine is in the session's own trust domain, so this half proves nothing
//! about identity: it moves the sandbox token into the central half's header and lets hermit-jarvis decide.
//!
//! - `http://magpie/…`, `http://hermit/…` (absolute form): to `<gateway URL>/magpie/…` / `/hermit/…` with the token
//!   and the Access service token.
//! - `http://model/…`: `GET /v1/aviary/models` from the session's model; `POST /v1/messages[/count_tokens]` to
//!   Anthropic with the session's CURRENT Claude login, read from `~/.claude/.credentials.json` per request (as
//!   api-proxy.js does: the router refreshes it, an account switch takes effect at the next request) and the OAuth beta
//!   that Claude Code sends only with its own OAuth login (the supervisor gives it a placeholder API token).
//! - `http://tools/…`: none in Jarvis 4 (404).
//! - anything else, and `CONNECT host:port`: the internet, directly (a session machine's egress is open).
//! - origin-form `POST /jarvis/deliver` (from harness-send on this machine): Jarvis's prompt into the conversation.
//!
//! At start it boots the sandbox (`POST /jarvis/boot`, signed with the machine key by `jarvis-machine sign-request`),
//! writes the supervisor's boot environment to a file for the harness, and keeps reading the supervisor's `:7070`
//! health to tell hermit when it is ready.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::TryStreamExt as _;
use http_body_util::{BodyExt as _, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::header::{self, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::json;
use tokio::io::AsyncWriteExt as _;
use tokio::net::{TcpListener, TcpStream};

use crate::wire::{self, BootReply, BootRequest, HealthReport, TOKEN_HEADER};

type Body = BoxBody<Bytes, std::io::Error>;

/// The proxy's address.
pub const LISTEN: &str = "127.0.0.1:3128";
/// Anthropic's API.
pub const ANTHROPIC: &str = "https://api.anthropic.com";
/// The beta Claude Code adds for an OAuth (subscription) login.
pub const OAUTH_BETA: &str = "oauth-2025-04-20";

/// What the gateway was started with.
#[derive(Clone, Debug)]
pub struct Config {
    /// `https://chat-gw.deyaochen.com`.
    pub gateway_url: String,
    pub access_id: String,
    pub access_secret: String,
    pub session: String,
    /// `claude-opus-5-5` (from `SESSION_MODEL` `aviary/<id>`).
    pub model: String,
    pub credentials: PathBuf,
    /// Where the supervisor's boot environment goes (mode 600).
    pub boot_file: PathBuf,
    pub image: String,
    pub env: BTreeMap<String, String>,
}

struct State {
    cfg: Config,
    boot: BootReply,
    http: reqwest::Client,
    /// The supervisor's last health answer (for the harness's status shim).
    health: std::sync::Mutex<Option<HealthReport>>,
}

fn full(status: StatusCode, body: serde_json::Value) -> Response<Body> {
    let mut r = Response::new(
        Full::new(Bytes::from(body.to_string()))
            .map_err(|never| match never {})
            .boxed(),
    );
    *r.status_mut() = status;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    r
}

fn refuse(status: StatusCode, code: &str, message: &str) -> Response<Body> {
    full(status, json!({"code": code, "message": message}))
}

fn http_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .tls_backend_preconfigured(aviary_core::http::tls_config())
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(90))
        .user_agent(concat!("aviary-gateway/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// The machine's memory and CPUs, for the descriptor.
fn machine_size() -> (u64, u32) {
    let mem = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|m| {
            m.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
        })
        .map_or(4 << 30, |kb| kb * 1024);
    let cpus =
        std::thread::available_parallelism().map_or(2, |n| u32::try_from(n.get()).unwrap_or(2));
    (mem, cpus)
}

/// Signs `POST <path>` over `body` with the machine key: `jarvis-machine sign-request` prints the router's headers.
async fn sign(path: &str, body: &[u8]) -> anyhow::Result<BTreeMap<String, String>> {
    let mut child = tokio::process::Command::new("jarvis-machine")
        .args(["sign-request", "POST", path])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(body).await?;
    }
    let out = child.wait_with_output().await?;
    if !out.status.success() {
        anyhow::bail!("jarvis-machine sign-request exited {}", out.status);
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

/// Boots the sandbox, retrying until hermit answers (a machine is useless without it).
async fn boot(cfg: &Config, http: &reqwest::Client) -> BootReply {
    let (memory_bytes, cpus) = machine_size();
    let req = BootRequest {
        session: cfg.session.clone(),
        image: cfg.image.clone(),
        memory_bytes,
        cpus,
        env: cfg
            .env
            .iter()
            .filter(|(k, _)| wire::BOOT_ENV_NAMES.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    };
    let body = serde_json::to_vec(&req).unwrap_or_default();
    let mut wait = Duration::from_secs(1);
    loop {
        let attempt = async {
            let headers = sign(wire::BOOT, &body).await?;
            let mut r = http
                .post(format!("{}{}", cfg.gateway_url, wire::BOOT))
                .header("CF-Access-Client-Id", &cfg.access_id)
                .header("CF-Access-Client-Secret", &cfg.access_secret)
                .header(header::CONTENT_TYPE, "application/json")
                .timeout(Duration::from_secs(60));
            for (k, v) in &headers {
                r = r.header(k.as_str(), v.as_str());
            }
            let r = r.body(body.clone()).send().await?;
            let status = r.status();
            if !status.is_success() {
                anyhow::bail!(
                    "boot answered {status}: {}",
                    r.text().await.unwrap_or_default()
                );
            }
            Ok::<_, anyhow::Error>(r.json::<BootReply>().await?)
        };
        match attempt.await {
            Ok(b) => return b,
            Err(e) => {
                tracing::warn!(error = %e, retry_in_s = wait.as_secs(), "boot failed");
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(Duration::from_secs(30));
            }
        }
    }
}

/// The supervisor's boot environment (DE0CH/aviary docs/CONTRACTS.md "Addressing"), as `KEY=VALUE` lines.
fn boot_env(b: &BootReply) -> String {
    let mut s = String::new();
    for (k, v) in gateway_api::credential::boot_env(&b.token, LISTEN) {
        s.push_str(k);
        s.push('=');
        s.push_str(&v);
        s.push('\n');
    }
    s.push_str(&format!(
        "AVIARY_SANDBOX_ID={}\nAVIARY_CONVERSATION_ID={}\n",
        b.sandbox_id, b.conversation_id
    ));
    s
}

/// Runs the gateway for ever.
///
/// # Errors
/// The listener cannot be bound or the boot file written.
pub async fn run(cfg: Config) -> anyhow::Result<()> {
    let http = http_client()?;
    let listener = TcpListener::bind(LISTEN).await?;
    let b = boot(&cfg, &http).await;
    tracing::info!(sandbox = %b.sandbox_id, conversation = %b.conversation_id, "booted");
    write_private(&cfg.boot_file, boot_env(&b).as_bytes())?;
    let state = Arc::new(State {
        cfg,
        boot: b,
        http,
        health: std::sync::Mutex::new(None),
    });
    tokio::spawn(health_loop(state.clone()));
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(error = %e, "accept");
                continue;
            }
        };
        let state = state.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let svc = hyper::service::service_fn(move |req| {
                let state = state.clone();
                async move { Ok::<_, Infallible>(handle(state, req).await) }
            });
            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .preserve_header_case(true)
                .serve_connection(io, svc)
                .with_upgrades()
                .await
            {
                tracing::debug!(error = %e, "connection");
            }
        });
    }
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        std::io::Write::write_all(&mut f, bytes)?;
    }
    std::fs::rename(tmp, path)
}

async fn handle(state: Arc<State>, req: Request<Incoming>) -> Response<Body> {
    if req.method() == Method::CONNECT {
        return connect(req);
    }
    let Some(host) = req.uri().authority().map(|a| a.as_str().to_owned()) else {
        // origin form: the gateway's own paths
        return own(&state, req).await;
    };
    let pq = req
        .uri()
        .path_and_query()
        .map_or("/", |p| p.as_str())
        .to_owned();
    match gateway_api::FixedName::from_host(&host) {
        Some(gateway_api::FixedName::Magpie) => to_central(&state, "magpie", &pq, req).await,
        Some(gateway_api::FixedName::Hermit) => to_central(&state, "hermit", &pq, req).await,
        Some(gateway_api::FixedName::Model) => model(&state, &pq, req).await,
        Some(gateway_api::FixedName::Tools) => {
            refuse(StatusCode::NOT_FOUND, "not_found", "no tools in Jarvis 4")
        }
        None => internet(&state, req).await,
    }
}

/// `CONNECT host:port`: a tunnel to the internet (never to a fixed name).
fn connect(req: Request<Incoming>) -> Response<Body> {
    let Some(target) = req.uri().authority().map(|a| a.as_str().to_owned()) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "CONNECT needs host:port",
        );
    };
    let host = target.rsplit_once(':').map_or(target.as_str(), |(h, _)| h);
    if gateway_api::FixedName::ALL
        .iter()
        .any(|n| n.as_str().eq_ignore_ascii_case(host))
    {
        return refuse(
            StatusCode::FORBIDDEN,
            "not_allowed",
            "a fixed name is never tunnelled",
        );
    }
    tokio::spawn(async move {
        match hyper::upgrade::on(req).await {
            Ok(up) => match TcpStream::connect(&target).await {
                Ok(mut server) => {
                    let mut client = TokioIo::new(up);
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                }
                Err(e) => tracing::debug!(target = %target, error = %e, "connect"),
            },
            Err(e) => tracing::debug!(error = %e, "upgrade"),
        }
    });
    Response::new(
        http_body_util::Empty::new()
            .map_err(|never| match never {})
            .boxed(),
    )
}

fn hop(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-connection"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "content-length"
    )
}

fn body_of(req: Request<Incoming>) -> reqwest::Body {
    reqwest::Body::wrap_stream(
        http_body_util::BodyDataStream::new(req.into_body())
            .map_err(|e| std::io::Error::other(e.to_string())),
    )
}

async fn relay(r: Result<reqwest::Response, reqwest::Error>) -> Response<Body> {
    let r = match r {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "upstream unreachable");
            return refuse(
                StatusCode::BAD_GATEWAY,
                "upstream_unreachable",
                "the upstream did not answer",
            );
        }
    };
    let mut out = Response::builder().status(r.status().as_u16());
    for (k, v) in r.headers() {
        if !hop(k.as_str()) {
            out = out.header(k, v);
        }
    }
    let stream = r
        .bytes_stream()
        .map_err(|e| std::io::Error::other(e.to_string()));
    out.body(http_body_util::StreamBody::new(stream.map_ok(hyper::body::Frame::data)).boxed())
        .unwrap_or_else(|_| {
            refuse(
                StatusCode::BAD_GATEWAY,
                "upstream_unreachable",
                "a bad upstream answer",
            )
        })
}

async fn to_central(state: &State, name: &str, pq: &str, req: Request<Incoming>) -> Response<Body> {
    let method = req.method().clone();
    let mut r = state
        .http
        .request(
            method.clone(),
            format!("{}/{name}{pq}", state.cfg.gateway_url),
        )
        .header("CF-Access-Client-Id", &state.cfg.access_id)
        .header("CF-Access-Client-Secret", &state.cfg.access_secret)
        .header(TOKEN_HEADER, &state.boot.token);
    for (k, v) in req.headers() {
        if !hop(k.as_str()) {
            r = r.header(k, v);
        }
    }
    if method != Method::GET && method != Method::HEAD {
        r = r.body(body_of(req));
    }
    relay(r.send().await).await
}

/// The session's Claude login (`claudeAiOauth.accessToken`), read fresh.
fn claude_token(path: &std::path::Path) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    v.get("claudeAiOauth")?
        .get("accessToken")?
        .as_str()
        .map(str::to_owned)
}

async fn model(state: &State, pq: &str, req: Request<Incoming>) -> Response<Body> {
    let method = req.method().clone();
    if !gateway_api::FixedName::Model.allows(method.as_str(), pq) {
        return refuse(StatusCode::FORBIDDEN, "not_allowed", "not a model path");
    }
    if method == Method::GET {
        let mut m = crate::central::models();
        m.default.clone_from(&state.cfg.model);
        return full(StatusCode::OK, serde_json::to_value(m).unwrap_or_default());
    }
    let Some(token) = claude_token(&state.cfg.credentials) else {
        return refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "no Claude login on this machine",
        );
    };
    let mut r = state.http.request(method, format!("{ANTHROPIC}{pq}"));
    let mut betas: Vec<String> = Vec::new();
    for (k, v) in req.headers() {
        let n = k.as_str();
        if hop(n) || n == "authorization" || n == "x-api-key" {
            continue;
        }
        if n == "anthropic-beta" {
            betas.extend(
                v.to_str()
                    .unwrap_or_default()
                    .split(',')
                    .map(|b| b.trim().to_owned()),
            );
            continue;
        }
        r = r.header(k, v);
    }
    if !betas.iter().any(|b| b == OAUTH_BETA) {
        betas.push(OAUTH_BETA.to_owned());
    }
    betas.retain(|b| !b.is_empty());
    r = r
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("anthropic-beta", betas.join(","))
        .body(body_of(req));
    relay(r.send().await).await
}

async fn internet(state: &State, req: Request<Incoming>) -> Response<Body> {
    let method = req.method().clone();
    let url = req.uri().to_string();
    let mut r = state.http.request(method.clone(), url);
    for (k, v) in req.headers() {
        if !hop(k.as_str()) {
            r = r.header(k, v);
        }
    }
    if method != Method::GET && method != Method::HEAD {
        r = r.body(body_of(req));
    }
    relay(r.send().await).await
}

/// Origin-form requests to the gateway itself (only from this machine: it listens on 127.0.0.1).
async fn own(state: &State, req: Request<Incoming>) -> Response<Body> {
    match (req.method().clone(), req.uri().path()) {
        (Method::POST, wire::DELIVER) => {
            let body = match req.into_body().collect().await {
                Ok(b) => b.to_bytes(),
                Err(_) => return refuse(StatusCode::BAD_REQUEST, "bad_request", "no body"),
            };
            let r = state
                .http
                .post(format!("{}{}", state.cfg.gateway_url, wire::DELIVER))
                .header("CF-Access-Client-Id", &state.cfg.access_id)
                .header("CF-Access-Client-Secret", &state.cfg.access_secret)
                .header(TOKEN_HEADER, &state.boot.token)
                .header(header::CONTENT_TYPE, "application/json")
                .timeout(Duration::from_secs(30))
                .body(body)
                .send()
                .await;
            relay(r).await
        }
        (Method::GET, "/jarvis/state") => {
            let h = state.health.lock().ok().and_then(|h| h.clone());
            full(
                StatusCode::OK,
                json!({
                    "sandboxId": state.boot.sandbox_id,
                    "conversationId": state.boot.conversation_id,
                    "ready": h.as_ref().is_some_and(|h| h.ok && h.ready),
                    "busy": h.as_ref().is_some_and(|h| h.busy),
                }),
            )
        }
        _ => refuse(StatusCode::NOT_FOUND, "not_found", "no such path"),
    }
}

/// Reads the supervisor's health every 2 s (it is local and cheap: the status shim's busy/idle comes from it) and tells
/// hermit while it starts, then every 15 s.
async fn health_loop(state: Arc<State>) {
    let url = format!(
        "http://127.0.0.1:{}{}",
        supervisor_api::CONTROL_PORT,
        supervisor_api::routes::HEALTH
    );
    let protocol = format!(
        "{}={}",
        supervisor_api::CONTROL.0,
        supervisor_api::CONTROL.1
    );
    let mut ready = false;
    let mut last_told = tokio::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let h = state
            .http
            .get(&url)
            .bearer_auth(&state.boot.control_token)
            .header("aviary-protocol", &protocol)
            .timeout(Duration::from_secs(3))
            .send()
            .await;
        let Ok(h) = h else { continue };
        let Ok(h) = h.json::<HealthReport>().await else {
            continue;
        };
        if let Ok(mut slot) = state.health.lock() {
            *slot = Some(h.clone());
        }
        if ready && h.ready && last_told.elapsed() < Duration::from_secs(15) {
            continue;
        }
        last_told = tokio::time::Instant::now();
        let r = state
            .http
            .post(format!("{}{}", state.cfg.gateway_url, wire::HEALTH))
            .header("CF-Access-Client-Id", &state.cfg.access_id)
            .header("CF-Access-Client-Secret", &state.cfg.access_secret)
            .header(TOKEN_HEADER, &state.boot.token)
            .timeout(Duration::from_secs(10))
            .json(&h)
            .send()
            .await;
        match r {
            Ok(r) if r.status().is_success() => ready = h.ready,
            Ok(r) => tracing::warn!(status = %r.status(), "health report refused"),
            Err(e) => tracing::warn!(error = %e, "health report failed"),
        }
    }
}

/// The address the gateway listens on.
///
/// # Panics
/// Never: [`LISTEN`] is a constant address.
#[must_use]
pub fn listen_addr() -> SocketAddr {
    LISTEN
        .parse()
        .unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], 3128)))
}
