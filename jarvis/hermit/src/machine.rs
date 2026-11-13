//! `aviary-gateway`: each aviary session machine's half of the sandbox gateway, on 127.0.0.1:3128 (the supervisor's
//! and Claude Code's `HTTP_PROXY`). The machine is in the session's own trust domain, so this half proves nothing
//! about identity: it moves the sandbox token into the central half's header and lets hermit-jarvis decide.
//!
//! - `http://magpie/…`, `http://hermit/…` (absolute form): to `<gateway URL>/magpie/…` / `/hermit/…` with the token
//!   and the Access service token.
//! - `http://model/…`: `GET /v1/aviary/models` from the session's model; `POST /v1/messages[/count_tokens]` by the
//!   body's `model`. A Claude model: to Anthropic with the session's CURRENT Claude login, read from
//!   `~/.claude/.credentials.json` per request (as api-proxy.js does: the router refreshes it, an account switch takes
//!   effect at the next request) and the OAuth beta that Claude Code sends only with its own OAuth login (the
//!   supervisor gives it a placeholder API token). An OpenRouter model (`maker/model`, app.rs): to OpenRouter's
//!   Messages API with `OPENROUTER_API` (store `openrouter`), the body as [`for_openrouter`] makes it.
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
/// OpenRouter's API (its Messages API is `/v1/messages` under it).
pub const OPENROUTER: &str = "https://openrouter.ai/api";
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
    /// Where model calls go: [`ANTHROPIC`], or `AVIARY_MODEL_URL` (the e2e's stand-in model, infra/e2e-aviary.sh).
    pub model_url: String,
    /// Where an OpenRouter model's calls go: [`OPENROUTER`], or `AVIARY_OPENROUTER_URL` (the e2e's stand-in).
    pub openrouter_url: String,
    /// `OPENROUTER_API` (store `openrouter`); without it an OpenRouter model's call is refused.
    pub openrouter_key: Option<String>,
}

struct State {
    cfg: Config,
    boot: BootReply,
    /// The deployment's settings, as the boot reply gave them: the models route and each model's routing.
    settings: crate::settings::Settings,
    http: reqwest::Client,
    /// The supervisor's last health answer (for the harness's status shim).
    health: std::sync::Mutex<Option<HealthReport>>,
    /// The conversation's title as hermit last said it (for the shim: the session's card in the Jarvis 4 app).
    title: std::sync::Mutex<Option<String>>,
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
            let reply = r.json::<BootReply>().await?;
            // the deployment's settings come with the boot: without them no model can be routed
            anyhow::ensure!(
                reply.settings.is_some(),
                "hermit-jarvis sent no settings (it predates them)"
            );
            Ok::<_, anyhow::Error>(reply)
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
        settings: b
            .settings
            .clone()
            .unwrap_or_else(|| unreachable!("boot() returns only a reply with settings")),
        cfg,
        boot: b,
        http,
        health: std::sync::Mutex::new(None),
        title: std::sync::Mutex::new(None),
    });
    tokio::spawn(health_loop(state.clone()));
    serve(listener, state).await
}

/// The proxy's accept loop.
async fn serve(listener: TcpListener, state: Arc<State>) -> anyhow::Result<()> {
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

fn stream_of(body: Incoming) -> reqwest::Body {
    reqwest::Body::wrap_stream(
        http_body_util::BodyDataStream::new(body).map_err(|e| std::io::Error::other(e.to_string())),
    )
}

fn body_of(req: Request<Incoming>) -> reqwest::Body {
    stream_of(req.into_body())
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
        let mut m = state.settings.models_route();
        m.default.clone_from(&state.cfg.model);
        return full(StatusCode::OK, serde_json::to_value(m).unwrap_or_default());
    }
    let (parts, incoming) = req.into_parts();
    let Ok(body) = incoming
        .collect()
        .await
        .map(http_body_util::Collected::to_bytes)
    else {
        return refuse(StatusCode::BAD_REQUEST, "bad_request", "no body");
    };
    if model_of(&body).is_some_and(|m| crate::settings::openrouter(&m)) {
        return openrouter(state, method, pq, &parts.headers, &body).await;
    }
    let Some(token) = claude_token(&state.cfg.credentials) else {
        return refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "no Claude login on this machine",
        );
    };
    // a call that is not Claude Code's own (the supervisor's title call) gets Claude Code's first system line, without
    // which Anthropic refuses a subscription login (429 `rate_limit_error`); Claude Code's own bodies go on as they are
    let own_call = parts
        .headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ua| ua.starts_with("claude-cli/"));
    let body = if own_call {
        reqwest::Body::from(body)
    } else {
        reqwest::Body::from(with_identity(&body).map_or(body, Bytes::from))
    };
    let mut r = state
        .http
        .request(method, format!("{}{pq}", state.cfg.model_url));
    let mut betas: Vec<String> = Vec::new();
    for (k, v) in &parts.headers {
        let n = k.as_str();
        // content-length: reqwest sets the body's own (a body given the identity line is longer)
        if hop(n) || n == "authorization" || n == "x-api-key" || n == "content-length" {
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
        .body(body);
    relay(r.send().await).await
}

/// A Messages body's `model`.
fn model_of(body: &[u8]) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct OnlyModel {
        model: Option<String>,
    }
    serde_json::from_slice::<OnlyModel>(body).ok()?.model
}

/// An OpenRouter model's call: to OpenRouter's Messages API with the machine's key. Nothing of the session's Claude
/// login goes there (the caller's credentials are dropped as on the Anthropic path).
async fn openrouter(
    state: &State,
    method: Method,
    pq: &str,
    headers: &hyper::HeaderMap,
    body: &Bytes,
) -> Response<Body> {
    let Some(key) = state.cfg.openrouter_key.as_deref() else {
        return refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "no OpenRouter key on this machine (OPENROUTER_API, store openrouter)",
        );
    };
    let body = for_openrouter(body, &state.settings).map_or_else(|| body.clone(), Bytes::from);
    let mut r = state
        .http
        .request(method, format!("{}{pq}", state.cfg.openrouter_url));
    for (k, v) in headers {
        let n = k.as_str();
        if hop(n) || n == "authorization" || n == "x-api-key" || n == "content-length" {
            continue;
        }
        r = r.header(k, v);
    }
    r = r
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .body(body);
    relay(r.send().await).await
}

/// A Messages body as OpenRouter gets it, by the deployment's settings for its model: the model's hosts first
/// (`provider.order`), Claude Code's effort level as OpenRouter's `reasoning.effort` when it is one of the model's
/// levels, and no `thinking: {type: "disabled"}` for a model with no level `none` (its hosts refuse a call that
/// switches thinking off; it then thinks at its own level). A model the settings do not list goes as it came, minus
/// a switched-off thinking. `None` when the body is not a JSON object.
#[must_use]
pub fn for_openrouter(body: &[u8], settings: &crate::settings::Settings) -> Option<Vec<u8>> {
    let mut v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let o = v.as_object_mut()?;
    let listed = settings.model(o.get("model").and_then(|m| m.as_str()).unwrap_or_default());
    if let Some(hosts) = listed
        .and_then(|m| m.openrouter.as_ref())
        .map(|r| &r.providers)
        .filter(|hosts| !hosts.is_empty())
    {
        o.insert("provider".to_owned(), json!({"order": hosts}));
    }
    let disabled = o
        .get("thinking")
        .and_then(|t| t.get("type"))
        .and_then(|t| t.as_str())
        == Some("disabled");
    if disabled && listed.is_none_or(|m| !m.thinking.iter().any(|l| l == "none")) {
        o.remove("thinking");
    }
    let effort = o
        .get("output_config")
        .and_then(|c| c.get("effort"))
        .and_then(|e| e.as_str())
        .filter(|e| listed.is_none_or(|m| m.thinking.iter().any(|l| l == e)))
        .map(str::to_owned);
    if let Some(effort) = effort {
        o.insert("reasoning".to_owned(), json!({"effort": effort}));
    }
    serde_json::to_vec(&v).ok()
}

/// The first line of Claude Code's system prompt: Anthropic takes a subscription (OAuth) login's Messages call only
/// when its system prompt begins with it (or with the Agent SDK's).
pub const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
const IDENTITY_STARTS: [&str; 2] = ["You are Claude Code", "You are a Claude agent"];

/// A Messages body whose system prompt does not begin with Claude Code's identity, with that line put first (the
/// caller's own system prompt follows as the next block); `None` when it already does, or the body is not a JSON
/// object (it goes on as it is).
#[must_use]
pub fn with_identity(body: &[u8]) -> Option<Vec<u8>> {
    let mut v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let o = v.as_object_mut()?;
    let block = |text: &str| json!({"type": "text", "text": text});
    let known = |t: &str| IDENTITY_STARTS.iter().any(|s| t.starts_with(s));
    let system = match o.remove("system") {
        None | Some(serde_json::Value::Null) => vec![block(CLAUDE_CODE_IDENTITY)],
        Some(serde_json::Value::String(t)) => {
            if known(&t) {
                return None;
            }
            vec![block(CLAUDE_CODE_IDENTITY), block(&t)]
        }
        Some(serde_json::Value::Array(mut blocks)) => {
            let first = blocks
                .first()
                .and_then(|b| b.get("text"))
                .and_then(serde_json::Value::as_str);
            if first.is_some_and(known) {
                return None;
            }
            blocks.insert(0, block(CLAUDE_CODE_IDENTITY));
            blocks
        }
        Some(_) => return None,
    };
    o.insert("system".into(), serde_json::Value::Array(system));
    serde_json::to_vec(&v).ok()
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
                    // what the session is doing, in the words Jarvis 4's registry takes (the harness's shim)
                    "status": h.as_ref().map(|h| jarvis_status(&h.state, h.busy)),
                    "title": state.title.lock().ok().and_then(|t| t.clone()),
                }),
            )
        }
        _ => refuse(StatusCode::NOT_FOUND, "not_found", "no such path"),
    }
}

/// The session's status as Jarvis 4's registry names it (router/registry.go), from the supervisor's run state. The
/// supervisor's `busy` is true while background tasks outlive a turn (`held`), which Jarvis showed as working: no
/// "idle with background jobs" on the card and no idle watchdog (Deyao, 2026-11-13). `held` is `shell` (the registry's
/// idle with at least one background task), `asking` is `waiting` (on Deyao's answer); a state this build does not
/// know goes by `busy`.
#[must_use]
pub fn jarvis_status(state: &str, busy: bool) -> &'static str {
    match state {
        "held" => "shell",
        "asking" => "waiting",
        "idle" => "idle",
        "starting" | "running" | "stopping" => "busy",
        _ if busy => "busy",
        _ => "idle",
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
            Ok(r) if r.status().is_success() => {
                ready = h.ready;
                // a title hermit names stays until it names another (an answer without one changes nothing)
                if let Ok(reply) = r.json::<wire::HealthReply>().await
                    && let Some(t) = reply.title
                    && let Ok(mut slot) = state.title.lock()
                {
                    *slot = Some(t);
                }
            }
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The deployment's settings as the tests' boot reply gives them (tests/settings.json).
    fn settings() -> crate::settings::Settings {
        crate::settings::Settings::parse(include_str!("../tests/settings.json")).unwrap()
    }

    /// Background tasks after a turn are idle with background jobs, an open question is waiting, never "working".
    #[test]
    fn the_supervisors_state_is_the_sessions_status() {
        assert_eq!(jarvis_status("running", true), "busy");
        assert_eq!(jarvis_status("starting", true), "busy");
        assert_eq!(jarvis_status("held", true), "shell");
        assert_eq!(jarvis_status("asking", false), "waiting");
        assert_eq!(jarvis_status("idle", false), "idle");
        // a supervisor that names no state, or one this build does not know: by its busy flag
        assert_eq!(jarvis_status("", true), "busy");
        assert_eq!(jarvis_status("", false), "idle");
        assert_eq!(jarvis_status("migrating", true), "busy");
    }

    fn system_of(body: &serde_json::Value) -> Option<serde_json::Value> {
        with_identity(&serde_json::to_vec(body).unwrap())
            .map(|b| serde_json::from_slice::<serde_json::Value>(&b).unwrap()["system"].clone())
    }

    /// The supervisor's title call (magpie's `title` command) has only the deployment's title instructions as its
    /// system prompt: Anthropic answered it 429 on a subscription login, so no conversation got a title (2026-11-12).
    #[test]
    fn a_call_without_claude_codes_identity_gets_it_first() {
        let title = json!({"model": "claude-haiku-5-5", "max_tokens": 64, "system": "Write a short title.",
            "messages": [{"role": "user", "content": "hello"}]});
        assert_eq!(
            system_of(&title),
            Some(
                json!([{"type": "text", "text": CLAUDE_CODE_IDENTITY}, {"type": "text", "text": "Write a short title."}])
            )
        );
        let rest: serde_json::Value =
            serde_json::from_slice(&with_identity(&serde_json::to_vec(&title).unwrap()).unwrap())
                .unwrap();
        assert_eq!(rest["messages"], title["messages"]);
        assert_eq!(rest["model"], title["model"]);
        // no system prompt at all, and one in blocks
        assert_eq!(
            system_of(&json!({"messages": []})),
            Some(json!([{"type": "text", "text": CLAUDE_CODE_IDENTITY}]))
        );
        assert_eq!(
            system_of(&json!({"system": [{"type": "text", "text": "Be brief."}], "messages": []})),
            Some(
                json!([{"type": "text", "text": CLAUDE_CODE_IDENTITY}, {"type": "text", "text": "Be brief."}])
            )
        );
    }

    #[test]
    fn claude_codes_own_bodies_are_left_alone() {
        for system in [
            json!("You are Claude Code, Anthropic's official CLI for Claude."),
            json!([{"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude."},
                   {"type": "text", "text": "more"}]),
            json!([{"type": "text", "text": "You are a Claude agent, built on Anthropic's Claude Agent SDK."}]),
        ] {
            assert_eq!(system_of(&json!({"system": system, "messages": []})), None);
        }
        assert_eq!(with_identity(b"not json"), None);
        assert_eq!(with_identity(b"[1]"), None);
    }

    /// GLM 5.3 on the chat's menu (Deyao, 2026-11-12): its calls go to OpenRouter, a Claude model's to Anthropic.
    #[test]
    fn a_call_is_openrouters_by_its_model() {
        let of =
            |m: &str| model_of(&serde_json::to_vec(&json!({"model": m, "messages": []})).unwrap());
        assert!(crate::settings::openrouter(&of("z-ai/glm-5.3").unwrap()));
        assert!(!crate::settings::openrouter(
            &of("claude-fable-5-2").unwrap()
        ));
        assert!(!crate::settings::openrouter(
            &of("claude-haiku-5-5").unwrap()
        ));
        assert_eq!(model_of(b"{}"), None);
        assert_eq!(model_of(b"not json"), None);
    }

    #[test]
    fn an_openrouter_body_names_the_hosts_and_the_effort_and_never_switches_thinking_off() {
        let to = |b: serde_json::Value| -> serde_json::Value {
            serde_json::from_slice(
                &for_openrouter(&serde_json::to_vec(&b).unwrap(), &settings()).unwrap(),
            )
            .unwrap()
        };
        // a turn at an effort level: the hosts in order, the level as OpenRouter's, the rest as it came
        let turn = json!({"model": "z-ai/glm-5.3", "max_tokens": 64, "thinking": {"type": "adaptive"},
            "output_config": {"effort": "xhigh"}, "messages": [{"role": "user", "content": "hi"}]});
        let out = to(turn.clone());
        assert_eq!(out["provider"], json!({"order": ["z-ai/fp8"]}));
        assert_eq!(out["reasoning"], json!({"effort": "xhigh"}));
        assert_eq!(out["thinking"], turn["thinking"]);
        assert_eq!(out["messages"], turn["messages"]);
        assert_eq!(out["model"], "z-ai/glm-5.3");
        // thinking switched off (the hosts answer that 400): the member goes, no level is named
        let off =
            to(json!({"model": "z-ai/glm-5.3", "thinking": {"type": "disabled"}, "messages": []}));
        assert_eq!(off.get("thinking"), None);
        assert_eq!(off.get("reasoning"), None);
        // a model with no hosts named gets no `provider`
        assert_eq!(
            to(json!({"model": "other/model", "messages": []})).get("provider"),
            None
        );
        // a level the model does not have is not passed on
        let odd = to(
            json!({"model": "z-ai/glm-5.3", "output_config": {"effort": "none"}, "messages": []}),
        );
        assert_eq!(odd.get("reasoning"), None);
        assert_eq!(for_openrouter(b"not json", &settings()), None);
        assert_eq!(for_openrouter(b"[1]", &settings()), None);
    }

    /// What a stand-in upstream saw: (authorization, anthropic-beta, body).
    type Seen = Arc<std::sync::Mutex<Vec<(String, String, serde_json::Value)>>>;

    /// A stand-in upstream on a free port: records each `POST /v1/messages` and answers a small message.
    async fn upstream() -> (String, Seen) {
        let seen: Seen = Arc::default();
        let s = seen.clone();
        let app = axum::Router::new().route(
            "/v1/messages",
            axum::routing::post(move |h: axum::http::HeaderMap, b: Bytes| {
                let s = s.clone();
                async move {
                    let get = |n: &str| {
                        h.get(n)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_owned()
                    };
                    s.lock().unwrap().push((
                        get("authorization"),
                        get("anthropic-beta"),
                        serde_json::from_slice(&b).unwrap(),
                    ));
                    axum::Json(
                        json!({"type": "message", "content": [{"type": "text", "text": "ok"}]}),
                    )
                }
            }),
        );
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await });
        (url, seen)
    }

    /// The gateway's proxy on a free port over two stand-ins (Anthropic, OpenRouter), and a client that uses it as
    /// its proxy, as Claude Code does.
    async fn gateway(openrouter_key: Option<&str>) -> (reqwest::Client, Seen, Seen) {
        let (anthropic, seen_a) = upstream().await;
        let (openrouter, seen_o) = upstream().await;
        let dir = std::env::temp_dir().join(format!("aviary-gw-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let credentials = dir.join("credentials.json");
        std::fs::write(
            &credentials,
            r#"{"claudeAiOauth": {"accessToken": "claude-login"}}"#,
        )
        .unwrap();
        let state = Arc::new(State {
            cfg: Config {
                gateway_url: "http://127.0.0.1:1".to_owned(),
                access_id: String::new(),
                access_secret: String::new(),
                session: "s".to_owned(),
                model: "z-ai/glm-5.3".to_owned(),
                credentials,
                boot_file: dir.join("boot.env"),
                image: String::new(),
                env: BTreeMap::new(),
                model_url: anthropic,
                openrouter_url: openrouter,
                openrouter_key: openrouter_key.map(str::to_owned),
            },
            boot: BootReply {
                sandbox_id: uuid::Uuid::new_v4(),
                conversation_id: uuid::Uuid::new_v4(),
                token: "hst1.test".to_owned(),
                control_token: "hct_test".to_owned(),
                settings: Some(settings()),
            },
            settings: settings(),
            http: http_client().unwrap(),
            health: std::sync::Mutex::new(None),
            title: std::sync::Mutex::new(None),
        });
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(serve(l, state));
        let client = reqwest::Client::builder()
            .tls_backend_preconfigured(aviary_core::http::tls_config())
            .proxy(reqwest::Proxy::http(proxy).unwrap())
            .build()
            .unwrap();
        (client, seen_a, seen_o)
    }

    async fn call(client: &reqwest::Client, body: &serde_json::Value) -> reqwest::StatusCode {
        client
            .post("http://model/v1/messages")
            .header("user-agent", "claude-cli/2.1.0 (external, cli)")
            .header("x-api-key", "placeholder")
            .header("anthropic-beta", "claude-code-20250219")
            .json(body)
            .send()
            .await
            .unwrap()
            .status()
    }

    /// GLM 5.3 on the chat's menu (Deyao, 2026-11-12): a turn on it reaches OpenRouter with the machine's key and
    /// the body OpenRouter needs; a Claude model's turn still reaches Anthropic with the session's login, untouched.
    #[tokio::test]
    async fn a_turn_goes_to_openrouter_or_anthropic_by_its_model() {
        let (client, seen_a, seen_o) = gateway(Some("or-key")).await;
        let glm = json!({"model": "z-ai/glm-5.3", "max_tokens": 64, "thinking": {"type": "disabled"},
            "output_config": {"effort": "high"}, "messages": [{"role": "user", "content": "hi"}]});
        assert_eq!(call(&client, &glm).await, StatusCode::OK);
        let claude = json!({"model": "claude-fable-5-2", "max_tokens": 64,
            "system": "You are Claude Code, Anthropic's official CLI for Claude.",
            "messages": [{"role": "user", "content": "hi"}]});
        assert_eq!(call(&client, &claude).await, StatusCode::OK);

        let o = seen_o.lock().unwrap().clone();
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].0, "Bearer or-key");
        assert_eq!(o[0].2["model"], "z-ai/glm-5.3");
        assert_eq!(o[0].2["provider"], json!({"order": ["z-ai/fp8"]}));
        assert_eq!(o[0].2["reasoning"], json!({"effort": "high"}));
        assert_eq!(o[0].2.get("thinking"), None);
        assert_eq!(o[0].2["messages"], glm["messages"]);

        let a = seen_a.lock().unwrap().clone();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].0, "Bearer claude-login");
        assert!(a[0].1.split(',').any(|b| b == OAUTH_BETA));
        assert!(a[0].1.split(',').any(|b| b == "claude-code-20250219"));
        assert_eq!(a[0].2, claude);
    }

    /// A machine without the store `openrouter` (a session made before the model was offered): the turn is refused
    /// here, and nothing of it reaches Anthropic.
    #[tokio::test]
    async fn an_openrouter_turn_without_the_key_is_refused() {
        let (client, seen_a, seen_o) = gateway(None).await;
        let glm = json!({"model": "z-ai/glm-5.3", "messages": []});
        assert_eq!(call(&client, &glm).await, StatusCode::SERVICE_UNAVAILABLE);
        assert!(seen_a.lock().unwrap().is_empty());
        assert!(seen_o.lock().unwrap().is_empty());
    }
}
